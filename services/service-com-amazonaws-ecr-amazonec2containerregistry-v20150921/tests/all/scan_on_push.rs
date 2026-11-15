// SPDX-License-Identifier: CC0-1.0

//! The basic scan a PUSH starts, and the event published when a scan finishes — end to end
//! over the wire.
//!
//! The rules under test:
//!
//! 1. A repository configured to scan images on push is scanned on every push: "the setting
//!    that determines whether images are scanned after being pushed to a repository. If set
//!    to `true`, images will be scanned after being pushed. If this parameter is not
//!    specified, it will default to `false` and images will not be scanned unless a scan is
//!    manually started" (`ImageScanningConfiguration$scanOnPush`). The run the push starts
//!    is the same run `StartImageScan` starts — it is recorded on the pushed image, it
//!    settles at the outcome of examining that image's content, it reads back through
//!    `DescribeImageScanFindings`, and it counts against the once-per-24-hours allowance
//!    that image has ("this limit includes if an image was scanned on initial push",
//!    `StartImageScan`).
//! 2. A scan that FINISHES is published onto the account's default event bus as an `ECR
//!    Image Scan` event of the source `aws.ecr`, whose `detail` carries the status the run
//!    ended in, the repository, the image's digest, that image's tags and the counts of its
//!    findings per severity. Those five members are what a rule's `EventPattern` filters on,
//!    so they are the contract this suite pins; one event is published per run that
//!    finishes, and the completion published is READ BACK out of the stored run — so a push
//!    that starts no run, a run that is still going, one that was replaced by a later start
//!    and an image that is gone each publish nothing.
//!
//! The rows differ along what the rules generalise over: whether the repository is scanned
//! on push (set true, set false, left unset), what the pushed image's filesystem carries (a
//! release file and a package database, neither), whether the push carries a tag, and — for
//! the published detail — the terminal status, how many tags the image has and how many
//! findings per severity the run reports.
//!
//! Nothing here sleeps for a scan to finish: it POLLS the reporting operation, exactly as a
//! client does.
//!
//! The wire harness is `crate::wire`'s: every case goes through the router, so it exercises
//! routing, deserialization, the handlers, the persisted state and serialization.

use std::collections::BTreeMap;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_ecr_amazonec2containerregistry_v20150921 as ecr;
use smithy_server_runtime::MultiServiceRouter;

// The layer harness and the two scan descriptions live in `crate::image_scan_filesystem`,
// the suite that pins what a basic scan makes of an image's filesystem: shared from there
// rather than copied, so the two suites cannot drift apart on either.
use crate::image_scan_filesystem::{
    Layer, ALPINE_OS_RELEASE, COMPLETE_DESCRIPTION, UNSUPPORTED_IMAGE_DESCRIPTION,
};
use crate::wire::{at, emulator_with_engine, given_layer, ok, ok_until, refused, REGISTRY_ID};

use ecr::state;
use ecr::state::image_events::{DEFAULT_EVENT_BUS, EVENT_SOURCE};
use ecr::state::scan_events::{completed_run, ScanCompletion, StartedScan, IMAGE_SCAN_DETAIL_TYPE};

/// What one pushed image's single layer carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filesystem {
    /// A release file and an installed-package database: what a basic scan reports on.
    Scannable,
    /// Bytes that are no filesystem archive at all.
    Opaque,
}

impl Filesystem {
    /// The layer an image carrying this filesystem is built on. A blob belongs to the
    /// repository it was uploaded to, so two rows may upload the same bytes.
    fn layer(self) -> Layer<'static> {
        match self {
            Self::Scannable => Layer::Gzipped(&[
                ("etc/os-release", ALPINE_OS_RELEASE),
                ("lib/apk/db/installed", "the apk database"),
            ]),
            Self::Opaque => Layer::Opaque("these bytes are no tar archive"),
        }
    }

    /// The terminal `ImageScanStatus` a basic scan of an image built on this filesystem
    /// settles at.
    fn terminal_status(self) -> Value {
        match self {
            Self::Scannable => json!({
                "status": state::SCAN_STATUS_COMPLETE,
                "description": COMPLETE_DESCRIPTION,
            }),
            Self::Opaque => json!({
                "status": state::SCAN_STATUS_FAILED,
                "description": UNSUPPORTED_IMAGE_DESCRIPTION,
            }),
        }
    }
}

/// One row of the scan-on-push table.
struct Row<'a> {
    /// What the row is about.
    case: &'a str,
    /// The repository it pushes its image into — its own, so no row depends on another.
    repository: &'a str,
    /// What the create's `imageScanningConfiguration.scanOnPush` says, or `None` where the
    /// create names no scanning configuration at all.
    scan_on_push: Option<bool>,
    /// What the pushed image's layer carries.
    filesystem: Filesystem,
    /// The tag the push carries, when it carries one.
    tag: Option<&'a str>,
}

impl Row<'_> {
    /// Whether this row's push owes the pushed image a scan.
    fn scans(&self) -> bool {
        self.scan_on_push.unwrap_or(false)
    }
}

/// Create `row`'s repository and push its image, answering the digest the registry
/// addresses that image by.
async fn given_pushed_image(router: &MultiServiceRouter, row: &Row<'_>) -> String {
    let mut create = json!({"repositoryName": row.repository});
    if let Some(scan_on_push) = row.scan_on_push {
        create["imageScanningConfiguration"] = json!({"scanOnPush": scan_on_push});
    }
    ok(router, row.case, "CreateRepository", &create).await;

    let bytes = row.filesystem.layer().bytes();
    let layer = given_layer(router, row.case, row.repository, &bytes).await;
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "layers": [{
            "mediaType": "application/vnd.docker.image.rootfs.diff.tar.gzip",
            "digest": layer,
            "size": bytes.len(),
        }],
    })
    .to_string();
    let mut push = json!({"repositoryName": row.repository, "imageManifest": manifest});
    if let Some(tag) = row.tag {
        push["imageTag"] = json!(tag);
    }
    ok(router, row.case, "PutImage", &push).await;
    state::manifest_digest(&manifest)
}

/// Poll `DescribeImageScanFindings` until the scan of `digest` reports a TERMINAL status —
/// the run has finished, either having examined the image or having been unable to — and
/// answer that whole read.
///
/// A freshly admitted run is queued and then runs, so neither `PENDING` nor `IN_PROGRESS` is
/// what this waits for: it waits for the condition the completion is, through the crate's
/// own poll helper, and never for a span of the clock.
async fn settled_scan(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    digest: &str,
) -> Value {
    ok_until(
        router,
        case,
        "DescribeImageScanFindings",
        &json!({"repositoryName": repository, "imageId": {"imageDigest": digest}}),
        |answer| {
            let status = at(answer, "/imageScanStatus/status");
            status == json!(state::SCAN_STATUS_COMPLETE) || status == json!(state::SCAN_STATUS_FAILED)
        },
    )
    .await
}

/// A push into a repository scanned on push starts the basic scan that image is owed, and a
/// push into one that is not scanned on push starts none.
#[tokio::test]
async fn a_push_starts_the_scan_the_repositorys_configuration_asks_for() {
    let rows = vec![
        Row {
            case: "scanned on push, an image a basic scan can examine",
            repository: "push-scan-alpine",
            scan_on_push: Some(true),
            filesystem: Filesystem::Scannable,
            tag: Some("v1"),
        },
        Row {
            case: "scanned on push, an image a basic scan cannot examine",
            repository: "push-scan-opaque",
            scan_on_push: Some(true),
            filesystem: Filesystem::Opaque,
            tag: Some("v1"),
        },
        Row {
            case: "scanned on push, pushed without a tag",
            repository: "push-scan-untagged",
            scan_on_push: Some(true),
            filesystem: Filesystem::Scannable,
            tag: None,
        },
        Row {
            case: "scanOnPush set false",
            repository: "push-no-scan",
            scan_on_push: Some(false),
            filesystem: Filesystem::Scannable,
            tag: Some("v1"),
        },
        Row {
            case: "no scanning configuration at all, so the modelled default decides",
            repository: "push-default",
            scan_on_push: None,
            filesystem: Filesystem::Scannable,
            tag: Some("v1"),
        },
    ];

    let (router, _engine) = emulator_with_engine();
    // Every row is pushed before any is read, so the rows that are scanned converge
    // together and the rows that are not are checked against a registry in which a scan
    // started by a push has already been reported.
    let mut pushed: Vec<(&Row<'_>, String)> = Vec::new();
    for row in &rows {
        let digest = given_pushed_image(&router, row).await;
        pushed.push((row, digest));
    }

    for (row, digest) in &pushed {
        if !row.scans() {
            continue;
        }
        let settled = settled_scan(&router, row.case, row.repository, digest).await;
        let expected = row.filesystem.terminal_status();
        assert_eq!(
            at(&settled, "/imageScanStatus"),
            expected,
            "case {:?}: the scan the push started must settle at {expected:?}, got {:?}",
            row.case,
            at(&settled, "/imageScanStatus")
        );
        assert_eq!(
            at(&settled, "/imageId/imageDigest"),
            json!(digest),
            "case {:?}: the scan must be the pushed image's own, got {:?}",
            row.case,
            at(&settled, "/imageId/imageDigest")
        );
        assert_eq!(
            at(&settled, "/registryId"),
            json!(REGISTRY_ID),
            "case {:?}: the read names this registry, got {:?}",
            row.case,
            at(&settled, "/registryId")
        );
        // The run the push started is the image's ONE run for the next 24 hours: "this
        // limit includes if an image was scanned on initial push".
        let refusal = refused(
            &router,
            row.case,
            "StartImageScan",
            &json!({"repositoryName": row.repository, "imageId": {"imageDigest": digest}}),
            "LimitExceededException",
        )
        .await;
        assert!(
            refusal["message"]
                .as_str()
                .unwrap_or_default()
                .contains("already been scanned"),
            "case {:?}: the refusal must say the image was already scanned, got {refusal:?}",
            row.case
        );
    }

    // The rows that are NOT scanned on push carry no scan: by now a push-started scan has
    // been read back for every row that owed one, so a scan these pushes started would
    // have been reported too.
    for (row, digest) in &pushed {
        if row.scans() {
            continue;
        }
        refused(
            &router,
            row.case,
            "DescribeImageScanFindings",
            &json!({"repositoryName": row.repository, "imageId": {"imageDigest": digest}}),
            "ScanNotFoundException",
        )
        .await;
        // And a manual scan of that image is still allowed, because nothing spent its
        // once-per-24-hours allowance.
        ok(
            &router,
            row.case,
            "StartImageScan",
            &json!({"repositoryName": row.repository, "imageId": {"imageDigest": digest}}),
        )
        .await;
    }
}

/// The registry-wide instruction asks for the same thing: a push into a repository a
/// `SCAN_ON_PUSH` rule of the registry's scanning configuration matches is scanned on
/// arrival, even where the repository's own configuration asks for nothing — and a
/// repository no rule matches is not.
///
/// The rows differ along what the rule generalises over: the scan type the registry is
/// configured for, the shape of the filter and the repository name it is matched against.
#[tokio::test]
async fn a_registry_scanning_rule_scans_the_repositories_it_matches_on_push() {
    // (case, the registry scan type, the rule's filters, the repository, whether a push
    // into it is scanned)
    let rows: Vec<(&str, &str, &[&str], &str, bool)> = vec![
        (
            "a wildcard filter matching the repository",
            "BASIC",
            &["reg-scan*"],
            "reg-scan-one",
            true,
        ),
        (
            "the same filter matching a namespaced name",
            "BASIC",
            &["reg-scan*"],
            "reg-scan/nested-two",
            true,
        ),
        (
            "a rule carrying no filter at all, which covers every repository",
            "BASIC",
            &[],
            "reg-scan-everything",
            true,
        ),
        (
            "a filter that names another repository",
            "BASIC",
            &["elsewhere*"],
            "reg-scan-unmatched",
            false,
        ),
        (
            // With the enhanced scan type the scans are Amazon Inspector's continuous
            // ones, not a basic scan this registry runs.
            "an enhanced-scanning registry, matching the repository",
            "ENHANCED",
            &["reg-scan*"],
            "reg-scan-enhanced",
            false,
        ),
    ];

    for (case, scan_type, filters, repository, scanned) in rows {
        let (router, _engine) = emulator_with_engine();
        ok(
            &router,
            case,
            "PutRegistryScanningConfiguration",
            &json!({
                "scanType": scan_type,
                "rules": [{
                    "scanFrequency": "SCAN_ON_PUSH",
                    "repositoryFilters": filters
                        .iter()
                        .map(|filter| json!({"filter": filter, "filterType": "WILDCARD"}))
                        .collect::<Vec<Value>>(),
                }],
            }),
        )
        .await;

        // The repository's OWN configuration asks for nothing, so whatever happens is the
        // registry-wide instruction's doing.
        let row = Row {
            case,
            repository,
            scan_on_push: None,
            filesystem: Filesystem::Opaque,
            tag: Some("v1"),
        };
        let digest = given_pushed_image(&router, &row).await;

        if scanned {
            let settled = settled_scan(&router, case, repository, &digest).await;
            assert_eq!(
                at(&settled, "/imageScanStatus"),
                Filesystem::Opaque.terminal_status(),
                "case {case:?}: the push must have started a scan, got {:?}",
                at(&settled, "/imageScanStatus")
            );
            continue;
        }

        // The negative rows get a positive control in the same registry first: a
        // repository whose own configuration asks for the scan is pushed to and read back,
        // so a scan the row's push had started would have been reported by now.
        let control = Row {
            case,
            repository: "reg-scan-control",
            scan_on_push: Some(true),
            filesystem: Filesystem::Opaque,
            tag: Some("v1"),
        };
        let control_digest = given_pushed_image(&router, &control).await;
        settled_scan(&router, case, control.repository, &control_digest).await;
        refused(
            &router,
            case,
            "DescribeImageScanFindings",
            &json!({"repositoryName": repository, "imageId": {"imageDigest": digest}}),
            "ScanNotFoundException",
        )
        .await;
    }
}

/// The identity of the event this registry publishes for a finished scan, as AWS names it.
#[test]
fn a_finished_scan_is_published_as_awss_own_event_identity() {
    assert_eq!(
        EVENT_SOURCE, "aws.ecr",
        "the source of an Amazon ECR event is aws.ecr, got {EVENT_SOURCE}"
    );
    assert_eq!(
        IMAGE_SCAN_DETAIL_TYPE, "ECR Image Scan",
        "the detail type of a finished image scan is 'ECR Image Scan', got \
         {IMAGE_SCAN_DETAIL_TYPE}"
    );
    assert_eq!(
        DEFAULT_EVENT_BUS, "default",
        "an event a service generates goes to the account's default bus, got \
         {DEFAULT_EVENT_BUS}"
    );
}

/// Every row: the `detail` the published event carries for one finished run, member for
/// member.
#[test]
fn the_published_detail_states_the_run_the_registry_finished() {
    // A fixed instant: the event's `time` is the run's completion instant, which no member
    // of the `detail` below is derived from, so nothing here reads a clock.
    let at_instant = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    // (case, the finished run, the detail the event must carry)
    let rows: Vec<(&str, ScanCompletion, Value)> = vec![
        (
            "a run that examined a tagged image and found nothing",
            ScanCompletion {
                repository_name: "orders".to_string(),
                repository_arn: "arn:aws:ecr:eu-west-1:000000000000:repository/orders".to_string(),
                image_digest: "sha256:aaaa".to_string(),
                image_tags: vec!["release".to_string()],
                scan_status: state::SCAN_STATUS_COMPLETE.to_string(),
                finding_severity_counts: BTreeMap::new(),
                at: at_instant,
            },
            json!({
                "scan-status": "COMPLETE",
                "repository-name": "orders",
                "image-digest": "sha256:aaaa",
                "image-tags": ["release"],
                "finding-severity-counts": {},
            }),
        ),
        (
            "a run reporting findings of two severities on an image with two tags",
            ScanCompletion {
                repository_name: "project-a/nginx".to_string(),
                repository_arn: "arn:aws:ecr:eu-west-1:000000000000:repository/project-a/nginx"
                    .to_string(),
                image_digest: "sha256:bbbb".to_string(),
                image_tags: vec!["latest".to_string(), "v2".to_string()],
                scan_status: state::SCAN_STATUS_COMPLETE.to_string(),
                finding_severity_counts: BTreeMap::from([
                    ("CRITICAL".to_string(), 2),
                    ("MEDIUM".to_string(), 7),
                ]),
                at: at_instant,
            },
            json!({
                "scan-status": "COMPLETE",
                "repository-name": "project-a/nginx",
                "image-digest": "sha256:bbbb",
                "image-tags": ["latest", "v2"],
                "finding-severity-counts": {"CRITICAL": 2, "MEDIUM": 7},
            }),
        ),
        (
            // An untagged image carries no tag, so the event states an empty list.
            "a run that could not examine an untagged image",
            ScanCompletion {
                repository_name: "payments".to_string(),
                repository_arn: "arn:aws:ecr:eu-west-1:000000000000:repository/payments"
                    .to_string(),
                image_digest: "sha256:cccc".to_string(),
                image_tags: Vec::new(),
                scan_status: state::SCAN_STATUS_FAILED.to_string(),
                finding_severity_counts: BTreeMap::new(),
                at: at_instant,
            },
            json!({
                "scan-status": "FAILED",
                "repository-name": "payments",
                "image-digest": "sha256:cccc",
                "image-tags": [],
                "finding-severity-counts": {},
            }),
        ),
    ];

    for (case, completion, expected) in rows {
        let detail = completion.detail();
        assert_eq!(
            detail, expected,
            "case {case:?}: expected the detail {expected}, got {detail}"
        );
    }
}

/// What is PUBLISHED for a finished run is read back out of the stored run: nothing is
/// reported while the run is still going, what is reported then is the record's own status,
/// tags and counts, and a run that was replaced or an image that is gone reports nothing.
#[tokio::test]
async fn the_published_completion_is_read_back_out_of_the_stored_run() {
    let (router, engine) = emulator_with_engine();
    let case = "a push that scanned its image";
    let row = Row {
        case,
        repository: "published-run",
        scan_on_push: Some(true),
        filesystem: Filesystem::Scannable,
        tag: Some("v1"),
    };
    let digest = given_pushed_image(&router, &row).await;

    let state = ecr::state::service_state(&*engine).expect("the ecr store is reachable");
    // The run the push recorded, named the way the push names it for its publication.
    let started = {
        let store = state.lock();
        let image = store
            .repository(crate::wire::REGION, row.repository)
            .ok()
            .and_then(|repo| repo.images.get(&digest))
            .unwrap_or_else(|| panic!("case {case:?}: the push must have stored the image"));
        let scan = image
            .scan
            .as_ref()
            .unwrap_or_else(|| panic!("case {case:?}: the push must have started a scan"));
        StartedScan {
            region: crate::wire::REGION.to_string(),
            repository_name: row.repository.to_string(),
            image_digest: digest.clone(),
            started_at: scan.started_at,
            complete_due_at: scan.complete_due_at,
        }
    };

    // Before the clock has made the completion due there is nothing to publish.
    assert!(
        completed_run(&state.lock(), &started, started.started_at).is_none(),
        "case {case:?}: a run that is still going must publish nothing"
    );

    // Once the run has settled — which the reporting operation is polled for, never slept
    // for — the completion reports that very record.
    let settled = settled_scan(&router, case, row.repository, &digest).await;
    // The instant the clock OWED this run its completion, read off the record itself: what
    // is published at that instant is what the record states, whatever the host's pace.
    let due = started.complete_due_at;
    let completion = completed_run(&state.lock(), &started, due)
        .unwrap_or_else(|| panic!("case {case:?}: a finished run must publish its completion"));
    assert_eq!(
        json!(completion.scan_status),
        at(&settled, "/imageScanStatus/status"),
        "case {case:?}: the published status must be the one the read reports, got {:?}",
        completion.scan_status
    );
    assert_eq!(
        completion.image_digest, digest,
        "case {case:?}: the published completion must name the scanned image, got {:?}",
        completion.image_digest
    );
    assert_eq!(
        completion.image_tags,
        vec!["v1".to_string()],
        "case {case:?}: the published completion must carry the image's tags, got {:?}",
        completion.image_tags
    );
    assert_eq!(
        completion.repository_name, row.repository,
        "case {case:?}: the published completion must name the repository, got {:?}",
        completion.repository_name
    );
    assert!(
        completion
            .repository_arn
            .ends_with(&format!(":repository/{}", row.repository)),
        "case {case:?}: the published completion must name the repository's ARN, got {:?}",
        completion.repository_arn
    );
    assert_eq!(
        completion.finding_severity_counts,
        BTreeMap::new(),
        "case {case:?}: the counts are over the findings the run reports, got {:?}",
        completion.finding_severity_counts
    );

    // A run that is not the one armed — a later start replaces it — publishes nothing in
    // that run's place.
    let replaced = StartedScan {
        started_at: started.started_at - Duration::from_secs(1),
        ..started.clone()
    };
    assert!(
        completed_run(&state.lock(), &replaced, due).is_none(),
        "case {case:?}: a run the image no longer carries must publish nothing"
    );

    // And an image the registry no longer holds publishes nothing either.
    ok(
        &router,
        case,
        "BatchDeleteImage",
        &json!({"repositoryName": row.repository, "imageIds": [{"imageDigest": digest}]}),
    )
    .await;
    assert!(
        completed_run(&state.lock(), &started, due).is_none(),
        "case {case:?}: a deleted image's run must publish nothing"
    );
}

/// WHICH run a push starts, and so which run it announces: the one it begins for an image
/// that is owed a scan, and NONE for an image that is not scanned again inside its
/// once-per-24-hours window — one event is published per scan that finishes, so a push that
/// starts no run announces nothing.
///
/// The cases differ along what the rule generalises over: whether the image carries a run at
/// all, whether the run it carries has finished, and whether the repository is scanned on
/// push.
#[tokio::test]
async fn a_push_announces_only_the_scan_it_starts() {
    let (router, engine) = emulator_with_engine();
    let case = "the run a push announces";
    let row = Row {
        case,
        repository: "announced-run",
        scan_on_push: Some(true),
        filesystem: Filesystem::Opaque,
        tag: Some("v1"),
    };
    let digest = given_pushed_image(&router, &row).await;
    let state = ecr::state::service_state(&*engine).expect("the ecr store is reachable");

    // The push above started the image's one run, which is what it announced.
    let first = {
        let store = state.lock();
        store
            .repository(crate::wire::REGION, row.repository)
            .ok()
            .and_then(|repo| repo.images.get(&digest))
            .and_then(|image| image.scan.clone())
            .unwrap_or_else(|| panic!("case {case:?}: the push must have started a scan"))
    };

    // A push that finds that run STILL GOING starts nothing and announces nothing: the run
    // was armed by the push that started it, and announcing it twice would report one run
    // twice.
    let while_running = state.lock().start_scan_on_push(
        crate::wire::REGION,
        row.repository,
        &digest,
        first.started_at,
    );
    assert!(
        while_running.is_none(),
        "case {case:?}: a run still going must not be announced again, got {while_running:?}"
    );

    // And once that run has FINISHED — polled for, never slept for — a push of the same
    // image still starts no second run inside its window, so it announces nothing either.
    settled_scan(&router, case, row.repository, &digest).await;
    let after_finishing = state.lock().start_scan_on_push(
        crate::wire::REGION,
        row.repository,
        &digest,
        first.complete_due_at,
    );
    assert!(
        after_finishing.is_none(),
        "case {case:?}: an image already scanned inside its window must not be scanned or \
         announced again, got {after_finishing:?}"
    );
    let carried = state
        .lock()
        .repository(crate::wire::REGION, row.repository)
        .ok()
        .and_then(|repo| repo.images.get(&digest))
        .and_then(|image| image.scan.clone())
        .unwrap_or_else(|| panic!("case {case:?}: the image must still carry its run"));
    assert_eq!(
        carried.started_at, first.started_at,
        "case {case:?}: no second run may be started inside the window, got {carried:?}"
    );

    // And a repository that is not scanned on push starts nothing, however the image got
    // there.
    let unscanned = Row {
        case,
        repository: "announced-none",
        scan_on_push: Some(false),
        filesystem: Filesystem::Opaque,
        tag: Some("v1"),
    };
    let other_digest = given_pushed_image(&router, &unscanned).await;
    let nothing = state.lock().start_scan_on_push(
        crate::wire::REGION,
        unscanned.repository,
        &other_digest,
        first.complete_due_at,
    );
    assert!(
        nothing.is_none(),
        "case {case:?}: a repository that is not scanned on push announces nothing, got \
         {nothing:?}"
    );
}
