// SPDX-License-Identifier: CC0-1.0

//! How many builds of ONE project may be in flight at a time, and the refusal a request
//! that would start one more gets.
//!
//! # The rule this module implements
//!
//! `Project$concurrentBuildLimit` is "the maximum number of concurrent builds that are
//! allowed for this project. New builds are only started if the current number of builds is
//! less than or equal to this limit. If the current build count meets this limit, new builds
//! are throttled and are not run." So the limit is a precondition of MINTING a build, not a
//! property of one that was minted: whichever operation is about to create a build of a
//! project that already holds that many builds in flight creates none and refuses, with the
//! modelled `AccountLimitExceededException` (client, HTTP 400) every build-starting
//! operation declares.
//!
//! What "in flight" counts is the builds a reader of this store sees as unfinished —
//! `Build$buildComplete` false, which is `buildStatus` `IN_PROGRESS` — because that is the
//! count the refusal's own remedy speaks about ("start the build again after other builds
//! are complete"): a caller that has read a build as complete is not refused on its account.
//! A build waiting for capacity ([`super::build_queue`]) is in flight too — it was created,
//! and it is `IN_PROGRESS` while it waits.
//!
//! A project that advertises no limit caps nothing: `concurrentBuildLimit` is absent until
//! it is set, and `UpdateProject` documents -1 as the value that REMOVES it rather than one
//! that caps anything, so a stored value below one build is not a cap a start can be refused
//! on.

use super::*;

/// The fewest concurrent builds a stored `concurrentBuildLimit` can cap a project at.
///
/// A limit is a count of builds that may run at once, and the member's own documentation
/// gives -1 as the value that removes the limit rather than one that caps anything
/// ("to remove this limit, set this value to -1"), so a stored value below one build caps
/// nothing here either.
const MIN_CONCURRENT_BUILD_LIMIT: i32 = 1;

/// What CodeBuild answers a request that would start a build of a project already holding
/// `limit` builds in flight: the limit it advertises, and the two ways out of it.
#[must_use]
pub fn concurrent_build_limit_message(limit: i32) -> String {
    format!(
        "Concurrent build limit exceeded. This build project has a limit of {limit} concurrent \
         build(s). Start the build again after other builds are complete, or edit project \
         configuration."
    )
}

impl ComAmazonawsCodebuildCodebuild20161006State {
    /// How many builds of `project_name` this store holds in flight: the ones a reader sees
    /// as unfinished.
    ///
    /// Counted through the project's own build index rather than by scanning every build, so
    /// the cost is the project's build count and not the store's.
    #[must_use]
    pub fn builds_in_flight(&self, project_name: &str) -> i64 {
        self.builds_by_project
            .get(project_name)
            .map_or(0, |ids| {
                ids.iter()
                    .filter(|id| {
                        self.builds
                            .get(*id)
                            .is_some_and(|stored| !stored.build.build_complete)
                    })
                    .count()
            })
            .try_into()
            .unwrap_or(i64::MAX)
    }

    /// The limit `project_name` advertises when starting one more build of it now would
    /// exceed it — `None` when the project caps nothing, or has room for another build.
    ///
    /// Read under the same guard the build is minted under, so two requests that arrive
    /// together cannot both find the last slot free.
    #[must_use]
    pub fn concurrent_build_limit_reached(&self, project_name: &str) -> Option<i32> {
        let limit = self
            .projects
            .get(project_name)?
            .project
            .concurrent_build_limit
            .filter(|limit| *limit >= MIN_CONCURRENT_BUILD_LIMIT)?;
        // "If the current build count meets this limit, new builds are throttled and are not
        // run": the build count MEETING the limit is what refuses the next build.
        (self.builds_in_flight(project_name) >= i64::from(limit)).then_some(limit)
    }
}
