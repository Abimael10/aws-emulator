// SPDX-License-Identifier: CC0-1.0

//! The SSM Agent BUILDS this emulator publishes, and which of them the guest of a managed
//! node holds.
//!
//! # Why a node does not simply report the current build
//!
//! "The version of SSM Agent running on your Linux managed node"
//! (`InstanceInformation$AgentVersion`) is a fact of the GUEST, not of this service, and
//! `InstanceInformation$IsLatestVersion` — "indicates whether the latest version of SSM Agent
//! is running on your Linux managed node" — is the comparison of that build with the newest
//! release the service publishes. A node whose guest holds an older build therefore reports
//! that older build and reports the comparison as false, which is the whole point of the
//! member: a client reconciling the agent versions of its fleet reads it to find the nodes
//! that are behind.
//!
//! What an EC2 instance's guest holds is the build the AMI it was launched from BUNDLES ("SSM
//! Agent is preinstalled … on Amazon Linux 2023"), which is the release that was current when
//! that image was last published. Amazon goes on publishing builds of an image family while
//! the family is in standard support — which is why the public parameter of a family resolves
//! to a fresh build — and publishes no further build once that support has ENDED, the instant
//! an image carries as its `Image$DeprecationTime`. So the instant that decides the build is
//! [`last_published_at`]: the deprecation instant for an image whose support has passed, and
//! the present for one still being published.
//!
//! # What is real here
//!
//! The builds below are the releases this emulator publishes, newest first, and the span
//! between two of them is CHOSEN ([`AGENT_RELEASE_INTERVAL`]): AWS publishes no cadence for
//! its agent releases, so this emulator fixes one, deliberately never zero so that an image
//! that stopped being published before the current release carries an earlier build. Nothing
//! here reads a clock of its own: every comparison is made against the instant the node
//! registered, which is the instant its agent reported the build it was running
//! ([`super::node_registration`]), so a node reports one build however often it is read.

use std::time::{Duration, SystemTime};

/// The SSM Agent builds this emulator publishes, NEWEST FIRST.
///
/// Each is a build of the agent, numbered the way the agent numbers its releases and matching
/// the modelled `Version` pattern `^[0-9]{1,6}(\.[0-9]{1,6}){2,3}$`. The first is the current
/// release — the build this emulator emulates — and each entry after it is one release older.
pub const AGENT_RELEASES: &[&str] = &[
    "3.3.2746.0",
    "3.3.1802.0",
    "3.3.1230.0",
    "3.2.2086.0",
    "3.2.1377.0",
];

/// How long before a release the release before it was published.
///
/// CHOSEN: AWS publishes agent builds on no documented cadence, so the span between the builds
/// of [`AGENT_RELEASES`] is this emulator's own — six weeks, and never zero, so an image that
/// stopped being published before the current release carries one of the earlier builds rather
/// than the current one.
pub const AGENT_RELEASE_INTERVAL: Duration = Duration::from_secs(42 * 24 * 60 * 60);

/// The current release: the build a node whose guest is up to date runs, and the one every
/// other build is compared against to answer `InstanceInformation$IsLatestVersion`.
pub const LATEST_AGENT_VERSION: &str = AGENT_RELEASES[0];

/// Whether `version` is the current release, which is what a node reports as its
/// `IsLatestVersion`.
#[must_use]
pub fn is_latest(version: &str) -> bool {
    version == LATEST_AGENT_VERSION
}

/// The build that was current `at`, as a node registering at `asked_at` reports it: the
/// current release, stepped back one build per [`AGENT_RELEASE_INTERVAL`] that has passed
/// since `at`.
///
/// An instant older than every build this emulator publishes answers the oldest of them: the
/// series is what this emulator knows, so a guest older than all of it holds the earliest
/// build it has.
#[must_use]
pub fn version_current_at(at: SystemTime, asked_at: SystemTime) -> &'static str {
    let elapsed = asked_at.duration_since(at).unwrap_or_default();
    let releases_since =
        usize::try_from(elapsed.as_secs() / AGENT_RELEASE_INTERVAL.as_secs()).unwrap_or(usize::MAX);
    AGENT_RELEASES[releases_since.min(AGENT_RELEASES.len() - 1)]
}

/// When the image whose `creation_date` and `deprecation_time` these are was LAST PUBLISHED,
/// for a reader asking at `asked_at`.
///
/// An image carries "the date and time to deprecate the AMI" (`Image$DeprecationTime`), which
/// for a published family is the instant its standard support ends: Amazon publishes no
/// further build of it after that, so the build its guest holds is the one that was current
/// then. An image whose deprecation instant has NOT passed — and one carrying none at all — is
/// still being published, so the instant that decides its build is the present. Neither
/// instant can precede the image's own `CreationDate`: an image is not published before it
/// exists.
///
/// Both are read through the ONE conversion Amazon EC2 reads its stored instants with, so this
/// service cannot disagree with that one about when an image was created or deprecated.
#[must_use]
pub fn last_published_at(
    creation_date: Option<&str>,
    deprecation_time: Option<&str>,
    asked_at: SystemTime,
) -> SystemTime {
    let instant = |rendering: Option<&str>| {
        ::service_com_amazonaws_ec2_amazonec2::state::parse_iso8601_timestamp(rendering)
            .and_then(|at| SystemTime::try_from(at).ok())
    };
    let published = match instant(deprecation_time) {
        Some(deprecated_at) if deprecated_at < asked_at => deprecated_at,
        _ => asked_at,
    };
    match instant(creation_date) {
        Some(created_at) if created_at > published => created_at,
        _ => published,
    }
}
