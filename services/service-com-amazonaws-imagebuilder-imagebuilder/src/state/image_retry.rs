// SPDX-License-Identifier: CC0-1.0

//! Which stopped build a retry may resume, and how a build it may not resume is refused.
//!
//! `RetryImage` "retries a failed or canceled image build without rebuilding the phases that
//! already completed. The image re-runs asynchronously in place: the same build version
//! returns to the TEST or DISTRIBUTION phase where it failed and continues from there" — so a
//! retry RESUMES a build from the phase that build stopped in, and the only phases it can
//! resume from are the two that sentence names. A build that stopped in any other phase has
//! no completed phases to resume after, which is why the state a build ended in does not
//! decide the question: a build stopped while it was still being built is a dead end whether
//! it was cancelled or failed.
//!
//! The phase itself is the stage of the image creation process the build was at, which this
//! service holds on the build version that stopped
//! ([`ImageBuildVersionRecord::stopped_in_phase`]): the modelled `ImageFailureContext`'s
//! `imageStatus` is "the status that the image had when the failure occurred. This indicates
//! the stage of the image creation process where the image failed, for example `BUILDING` or
//! `DISTRIBUTING`" (`smithy_query(query="shape",shape="ImageFailureContext")`), so a phase is
//! one of the modelled `ImageStatus` values and is read from this store rather than from the
//! request.

use std::time::SystemTime;

use super::{ImageBuildVersionRecord, IMAGE_ARN_MEMBER, TERMINAL_IMAGE_STATUSES};

/// The phase Image Builder runs the build of an image in, from the modelled `ImageStatus`
/// enum, which `ImageFailureContext$imageStatus` names a stage of the image creation process
/// ("for example `BUILDING` or `DISTRIBUTING`").
pub const IMAGE_PHASE_BUILDING: &str = "BUILDING";

/// The phase Image Builder tests a built image in, from the same enum: "the same build version
/// returns to the test or distribution phase where it failed and continues from there"
/// (`RetryImage`), so a build that stopped in it has a completed build phase to resume after.
pub const IMAGE_PHASE_TESTING: &str = "TESTING";

/// The phase Image Builder distributes a tested image in. See [`IMAGE_PHASE_TESTING`].
pub const IMAGE_PHASE_DISTRIBUTING: &str = "DISTRIBUTING";

/// The phases a stopped build may be RESUMED from, in the order a build runs them: the two
/// `RetryImage` names ("the same build version returns to the test or distribution phase where
/// it failed and continues from there"), which are the phases that have a completed build
/// phase behind them.
pub const RESUMABLE_IMAGE_PHASES: &[&str] = &[IMAGE_PHASE_TESTING, IMAGE_PHASE_DISTRIBUTING];

impl ImageBuildVersionRecord {
    /// The phase the build of this build version stopped in, read at the instant `at`, or
    /// `None` while that build has not stopped at all.
    ///
    /// A build is stopped once something ENDED it: a build still running has no phase it
    /// stopped in, and neither has one that ran every phase to completion, so the phase is
    /// read only for a build whose status at `at` is one something stopped it in
    /// ([`super::IMAGE_STATUS_CANCELLED`], [`super::IMAGE_STATUS_FAILED`]), which the clock
    /// decides ([`ImageBuildVersionRecord::status_at`]).
    ///
    /// A build this store recorded no phase for stopped in the phase every build of this
    /// emulator runs — the build phase: the work a build runs from the instant it was queued
    /// until it is due ([`super::IMAGE_BUILD_SPAN`]) is that phase, so a build stopped while it
    /// was outstanding stopped in it.
    #[must_use]
    pub fn stopped_in_phase_at(&self, at: SystemTime) -> Option<&str> {
        let status = self.status_at(at);
        if status == super::IMAGE_STATUS_AVAILABLE || !TERMINAL_IMAGE_STATUSES.contains(&status) {
            return None;
        }
        Some(
            self.stopped_in_phase
                .as_deref()
                .unwrap_or(IMAGE_PHASE_BUILDING),
        )
    }

    /// Whether a retry may RESUME the build of this build version, read at the instant `at`:
    /// the build stopped, and the phase it stopped in is one of the two a retry resumes from
    /// ([`RESUMABLE_IMAGE_PHASES`]).
    #[must_use]
    pub fn resumable_at(&self, at: SystemTime) -> bool {
        self.stopped_in_phase_at(at)
            .is_some_and(|phase| RESUMABLE_IMAGE_PHASES.contains(&phase))
    }
}

/// Why a retry of a build that stopped in a phase no retry resumes from is refused.
///
/// The requirement is the PHASE: a retry continues a build "from the test or distribution
/// phase where it failed", so a build that stopped anywhere else cannot be continued, and the
/// clause names the phase that build stopped in — read from this store's own record of that
/// build ([`ImageBuildVersionRecord::stopped_in_phase_at`]) — beside the phases that can be
/// resumed from ([`RESUMABLE_IMAGE_PHASES`]). Stated in the sentence this service states every
/// refused parameter in (`service_common::validation_message::parameter_value_mismatch`),
/// naming the parameter this operation's own validator addresses
/// ([`IMAGE_ARN_MEMBER`]).
#[must_use]
pub fn image_phase_not_retryable_message(stopped_in_phase: &str) -> String {
    ::service_common::validation_message::parameter_value_mismatch(
        IMAGE_ARN_MEMBER,
        &format!(
            "The provided image cannot be retried. Retry is not supported for images that \
             failed during '{stopped_in_phase}' phase. Supported phases: {}",
            RESUMABLE_IMAGE_PHASES.join(", ")
        ),
    )
}
