// SPDX-License-Identifier: CC0-1.0

//! WHICH SENTENCE a terminal basic scan reports, and what selects it — end to end over the
//! wire.
//!
//! The rule under test: a terminal basic scan's `imageScanStatus.description` states WHY the
//! run could not examine the image — or that it did — so it is a function of the scanned
//! image's content and of nothing else. A run that read the image's filesystem and found
//! nothing there to enumerate packages from reports the operating-system/package-manager
//! sentence, a run whose layers are no filesystem it can read at all reports the
//! image-not-supported sentence, and a run that examined the filesystem reports the
//! completion sentence: three distinct sentences, none of them a constant standing in for
//! another. Because the description belongs to the RUN, the same function decides it however
//! the run was started.
//!
//! The rows differ along what the rule generalises over: what the pushed image's filesystem
//! carries (a release file and a package database, a release file alone, a package database
//! alone, an archive with no member at all, bytes that are no archive) crossed with what
//! started the run (an explicit `StartImageScan`, a push into a repository whose own
//! `imageScanningConfiguration` scans on push, and a push into a repository a registry-wide
//! `SCAN_ON_PUSH` rule of a `BASIC` scanning configuration matches). The reported case — a
//! single-layer image whose layer is an empty gzipped tar — is one row of the content axis,
//! exercised on all three paths.
//!
//! The layer harness and the three sentences come from `crate::image_scan_filesystem`, where
//! they are spelled out rather than imported from the crate, so a change to the constants the
//! handler reads fails this suite too.
//!
//! The wire harness is `crate::wire`'s: every case goes through the router, so it exercises
//! routing, deserialization, the handlers, the persisted state and serialization — the same
//! path a client takes.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_ecr_amazonec2containerregistry_v20150921 as ecr;
use smithy_server_runtime::MultiServiceRouter;

use crate::image_scan_filesystem::{
    Layer, ALPINE_OS_RELEASE, COMPLETE_DESCRIPTION, UNSUPPORTED_FILESYSTEM_DESCRIPTION,
    UNSUPPORTED_IMAGE_DESCRIPTION,
};
use crate::wire::{at, emulator, given_layer, ok, ok_until};

use ecr::state;

/// The part of an unsupported run's description a client keys on: both unsupported sentences
/// are prefixed with it, which is what makes the sentence after it the reason.
const UNSUPPORTED_PREFIX: &str = "UnsupportedImageError: ";

/// The wildcard a registry-wide `SCAN_ON_PUSH` rule is filtered by here, so only the rows on
/// that path are scanned by it.
const REGISTRY_RULE_FILTER: &str = "desc-registry*";

/// What the one layer of a row's image carries — the content the outcome follows from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    /// A release file and an installed-package database: what a basic scan reports on.
    Scannable,
    /// A filesystem naming its operating system, with no package database to enumerate.
    ReleaseFileOnly,
    /// A filesystem carrying a package database but naming no operating system.
    PackageDatabaseOnly,
    /// An archive carrying no member at all: a filesystem the run READ and found empty.
    EmptyArchive,
    /// Bytes that are no filesystem archive, so no filesystem was read at all.
    NotAnArchive,
}

impl Content {
    /// Every content the rule is exercised over.
    const ALL: &'static [Self] = &[
        Self::Scannable,
        Self::ReleaseFileOnly,
        Self::PackageDatabaseOnly,
        Self::EmptyArchive,
        Self::NotAnArchive,
    ];

    /// The layer an image carrying this content is built on. A blob belongs to the
    /// repository it was uploaded to, so two rows may upload the same bytes.
    fn layer(self) -> Layer<'static> {
        match self {
            Self::Scannable => Layer::Gzipped(&[
                ("etc/os-release", ALPINE_OS_RELEASE),
                ("lib/apk/db/installed", "the apk database"),
            ]),
            Self::ReleaseFileOnly => Layer::Gzipped(&[
                ("etc/os-release", ALPINE_OS_RELEASE),
                ("usr/bin/app", "the application this image ships"),
            ]),
            Self::PackageDatabaseOnly => Layer::Gzipped(&[
                ("var/lib/dpkg/status", "Package: bash\nVersion: 5.2.15-2\n\n"),
                ("usr/bin/app", "the application this image ships"),
            ]),
            Self::EmptyArchive => Layer::Gzipped(&[]),
            Self::NotAnArchive => Layer::Opaque("these bytes are no tar archive"),
        }
    }

    /// The `imageScanStatus.status` a run on this content ends at.
    fn status(self) -> &'static str {
        match self {
            Self::Scannable => state::SCAN_STATUS_COMPLETE,
            _ => state::SCAN_STATUS_FAILED,
        }
    }

    /// The `imageScanStatus.description` that run reports: the sentence for WHY the image
    /// could not be examined, or the completion sentence where it was.
    fn description(self) -> &'static str {
        match self {
            Self::Scannable => COMPLETE_DESCRIPTION,
            // The filesystem was read; there was nothing in it to report packages on.
            Self::ReleaseFileOnly | Self::PackageDatabaseOnly | Self::EmptyArchive => {
                UNSUPPORTED_FILESYSTEM_DESCRIPTION
            }
            // Nothing read as a filesystem at all.
            Self::NotAnArchive => UNSUPPORTED_IMAGE_DESCRIPTION,
        }
    }

    /// The whole `imageScanStatus` a terminal run on this content answers with.
    fn terminal_status(self) -> Value {
        json!({"status": self.status(), "description": self.description()})
    }

    /// What this content is about, for a case name.
    fn describe(self) -> &'static str {
        match self {
            Self::Scannable => "a release file and a package database",
            Self::ReleaseFileOnly => "a release file and no package database",
            Self::PackageDatabaseOnly => "a package database and no release file",
            Self::EmptyArchive => "an archive carrying no member at all",
            Self::NotAnArchive => "bytes that are no archive",
        }
    }

    /// The part of a repository name that distinguishes this content's row.
    fn slug(self) -> &'static str {
        match self {
            Self::Scannable => "scannable",
            Self::ReleaseFileOnly => "release-only",
            Self::PackageDatabaseOnly => "packages-only",
            Self::EmptyArchive => "empty-archive",
            Self::NotAnArchive => "not-an-archive",
        }
    }
}

/// What started the run whose description is read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartedBy {
    /// An explicit `StartImageScan` call on the pushed image.
    AnExplicitStart,
    /// A push into a repository whose own `imageScanningConfiguration` scans on push.
    ARepositoryScanningOnPush,
    /// A push into a repository a registry-wide `SCAN_ON_PUSH` rule matches.
    ARegistryScanningRule,
}

impl StartedBy {
    /// Every path a basic run is started by here.
    const ALL: &'static [Self] = &[
        Self::AnExplicitStart,
        Self::ARepositoryScanningOnPush,
        Self::ARegistryScanningRule,
    ];

    /// What the create says about `imageScanningConfiguration.scanOnPush`, or `None` where
    /// the create names no scanning configuration at all.
    fn scan_on_push(self) -> Option<bool> {
        match self {
            Self::ARepositoryScanningOnPush => Some(true),
            // The registry-wide rule is the only instruction on that path, and the explicit
            // path must be the start call's own doing.
            Self::ARegistryScanningRule | Self::AnExplicitStart => None,
        }
    }

    /// The prefix of the repositories this path pushes into: the registry-rule rows are
    /// named so that [`REGISTRY_RULE_FILTER`] matches them and nothing else is.
    fn repository_prefix(self) -> &'static str {
        match self {
            Self::AnExplicitStart => "desc-explicit",
            Self::ARepositoryScanningOnPush => "desc-repository",
            Self::ARegistryScanningRule => "desc-registry",
        }
    }

    /// What this path is about, for a case name.
    fn describe(self) -> &'static str {
        match self {
            Self::AnExplicitStart => "started by StartImageScan",
            Self::ARepositoryScanningOnPush => "started by a push the repository scans",
            Self::ARegistryScanningRule => "started by a push a registry rule scans",
        }
    }
}

/// Create `repository` — with the row's scanning configuration, where it has one — and push
/// a Docker image manifest V2 schema 2 image built on `content`'s single layer into it,
/// answering the digest the registry addresses that image by.
async fn given_pushed_image(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    content: Content,
    scan_on_push: Option<bool>,
) -> String {
    let mut create = json!({"repositoryName": repository});
    if let Some(scan_on_push) = scan_on_push {
        create["imageScanningConfiguration"] = json!({"scanOnPush": scan_on_push});
    }
    ok(router, case, "CreateRepository", &create).await;

    let bytes = content.layer().bytes();
    let layer = given_layer(router, case, repository, &bytes).await;
    // The configuration blob names the repository, so no two rows push the same manifest.
    let config = format!("{{\"architecture\":\"amd64\",\"os\":\"linux\",\"rootfs\":\"{repository}\"}}");
    let config_digest = given_layer(router, case, repository, config.as_bytes()).await;
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {
            "mediaType": "application/vnd.docker.container.image.v1+json",
            "digest": config_digest,
            "size": config.len(),
        },
        "layers": [{
            "mediaType": "application/vnd.docker.image.rootfs.diff.tar.gzip",
            "digest": layer,
            "size": bytes.len(),
        }],
    })
    .to_string();
    ok(
        router,
        case,
        "PutImage",
        &json!({
            "repositoryName": repository,
            "imageManifest": manifest,
            "imageTag": "v1",
        }),
    )
    .await;
    state::manifest_digest(&manifest)
}

/// The terminal description a scan reports is the one its own outcome asks for, on every
/// path a basic scan is started by — so the two unsupported sentences are told apart by WHY
/// the image could not be examined and neither of them answers for the other.
#[tokio::test]
async fn the_terminal_description_states_why_the_image_was_not_examined() {
    let router = emulator();
    // The registry-wide instruction is in force before anything is pushed, and it is
    // filtered to the rows that are started by it.
    ok(
        &router,
        "the registry scanning configuration",
        "PutRegistryScanningConfiguration",
        &json!({
            "scanType": "BASIC",
            "rules": [{
                "scanFrequency": "SCAN_ON_PUSH",
                "repositoryFilters": [
                    {"filter": REGISTRY_RULE_FILTER, "filterType": "WILDCARD"},
                ],
            }],
        }),
    )
    .await;

    // (case, repository, digest, content) — every row is pushed and started before any is
    // read, so the runs the clock owes proceed together rather than one row at a time.
    let mut started: Vec<(String, String, String, Content)> = Vec::new();
    for started_by in StartedBy::ALL {
        for content in Content::ALL {
            let case = format!("{}, {}", content.describe(), started_by.describe());
            let repository = format!("{}-{}", started_by.repository_prefix(), content.slug());
            let digest = given_pushed_image(
                &router,
                &case,
                &repository,
                *content,
                started_by.scan_on_push(),
            )
            .await;
            if *started_by == StartedBy::AnExplicitStart {
                let start = ok(
                    &router,
                    &case,
                    "StartImageScan",
                    &json!({"repositoryName": repository, "imageId": {"imageDigest": digest}}),
                )
                .await;
                assert_eq!(
                    at(&start, "/imageScanStatus/status"),
                    json!(state::SCAN_STATUS_PENDING),
                    "case {case:?}: expected the admitted run to be queued, got {:?}",
                    at(&start, "/imageScanStatus")
                );
            }
            started.push((case, repository, digest, *content));
        }
    }

    // What each row reported, so the sentences can be compared against each other after the
    // table has run.
    let mut reported: BTreeSet<String> = BTreeSet::new();
    for (case, repository, digest, content) in &started {
        let expected = content.terminal_status();
        // The terminal status is the clock's to owe, so the suite looks again until the run
        // has ended rather than waiting out a span of wall clock. The poll waits for
        // TERMINALITY alone: which status and which sentence it ended with is the assertion's.
        let ended = ok_until(
            &router,
            case,
            "DescribeImageScanFindings",
            &json!({"repositoryName": repository, "imageId": {"imageDigest": digest}}),
            |answer| {
                [state::SCAN_STATUS_COMPLETE, state::SCAN_STATUS_FAILED]
                    .iter()
                    .any(|status| at(answer, "/imageScanStatus/status") == json!(status))
            },
        )
        .await;
        assert_eq!(
            at(&ended, "/imageScanStatus"),
            expected,
            "case {case:?}: expected the terminal status {expected:?}, got {:?}",
            at(&ended, "/imageScanStatus")
        );
        let description = at(&ended, "/imageScanStatus/description");
        let description = description
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a terminal run states a description"))
            .to_string();
        if content.status() == state::SCAN_STATUS_FAILED {
            assert!(
                description.starts_with(UNSUPPORTED_PREFIX),
                "case {case:?}: expected the description to carry the \
                 {UNSUPPORTED_PREFIX:?} prefix a client keys on, got {description:?}"
            );
        }
        reported.insert(description);
    }

    // The three sentences are three: a single constant for every unsupported image would
    // report fewer, and a client could not tell the two unsupported outcomes apart.
    let expected_sentences: BTreeSet<String> = Content::ALL
        .iter()
        .map(|content| content.description().to_string())
        .collect();
    assert_eq!(
        reported, expected_sentences,
        "case \"the whole table\": expected the three outcomes to report \
         {expected_sentences:?}, got {reported:?}"
    );
    assert_eq!(
        reported.len(),
        3,
        "case \"the whole table\": expected three distinct sentences across the outcomes, \
         got {reported:?}"
    );
}
