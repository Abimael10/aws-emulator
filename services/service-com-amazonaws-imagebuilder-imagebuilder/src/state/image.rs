// SPDX-License-Identifier: CC0-1.0

//! Which stored image build version a reference to an image reaches, and what a read of
//! that build version reports.
//!
//! An image is the one resource of this service a reference may reach in TWO ways, and
//! `GetImage`'s `imageBuildVersionArn` states both: "You can specify a full build version
//! ARN, or a version ARN with or without wildcards (x.x.x, 1.x.x, or 1.0.x). A version or
//! wildcard ARN resolves to the latest matching build version that has reached AVAILABLE
//! status. … To get an image in any other state, such as a failed or in-progress build,
//! specify the full build version ARN." So:
//!
//! * a BUILD version ARN names one build and reaches it whatever status it reports — which
//!   is what lets a caller watch the build it just queued;
//! * a VERSION ARN, wildcards admitted in trailing nodes as every versioned reference of
//!   this service admits them ([`super::VersionedReference`]), reaches the greatest matching
//!   version's greatest build among the builds that have reached
//!   [`super::IMAGE_STATUS_AVAILABLE`] — a status the CLOCK decides
//!   ([`ImageBuildVersionRecord::status_at`]), so an outstanding build is reached by its own
//!   build version ARN and by no version ARN until it is due.
//!
//! What a read reports is what the build RECORDED ([`ImageBuildVersionRecord`]), plus the
//! resources that record references, each read back from the store it is held in
//! ([`ComAmazonawsImagebuilderImagebuilderState::image_view`]) — so a recipe or a
//! configuration a read reports is one this service still holds, and one that has since been
//! deleted is reported by neither.

use std::time::SystemTime;

use crate::ops::types::{Image, ImageState};

use super::{
    managed_image_view, normalized_name, parse_semantic_version, reported_tags, version_admits,
    ComAmazonawsImagebuilderImagebuilderState, ContainerRecipeRecord,
    DistributionConfigurationRecord, ImageBuildVersionRecord, ImageRecipeRecord,
    InfrastructureConfigurationRecord, ARN_SERVICE, IMAGE_ARN_PREFIX, IMAGE_STATUS_AVAILABLE,
};

impl ImageBuildVersionRecord {
    /// The semantic version this build version reports: its three assigned nodes and the
    /// build number Image Builder assigned as the fourth.
    ///
    /// "The semantic version has four nodes: `<major>.<minor>.<patch>/<build>`. You can
    /// assign values for the first three … Image Builder automatically assigns the build
    /// number to the fourth node" (the modelled `version` member of `Image`), whose target
    /// shape is the one that admits that fourth node (`VersionNumberWithBuild`), unlike the
    /// version-level resource's (`ImageVersion`'s `version`, `VersionNumber`).
    #[must_use]
    pub fn semantic_version(&self) -> String {
        format!("{}/{}", self.version, self.build_version)
    }
}

impl ComAmazonawsImagebuilderImagebuilderState {
    /// The image the ARN `reference` reaches at the instant `at`, as the modelled `Image` a
    /// read of it reports, or `None` when it reaches none.
    ///
    /// An image ARN carries the account that OWNS the image, and the two owners a read's
    /// modelled ARN admits are read by their own rules: an ARN of THIS account reaches a build
    /// version this store holds ([`Self::resolve_image`]), and one of Amazon's published
    /// catalog reaches an image of that catalog ([`super::managed_image_view`]), which no
    /// build of this account ever files.
    #[must_use]
    pub fn image_answer(
        &self,
        reference: &str,
        request_region: &str,
        at: SystemTime,
    ) -> Option<Image> {
        if let Some(published) = managed_image_view(reference, request_region) {
            return Some(published);
        }
        self.resolve_image(reference, request_region, at)
            .map(|record| self.image_view(record, at))
    }

    /// The image build version the ARN `reference` reaches at the instant `at`, when one is
    /// stored in `request_region`.
    ///
    /// The two readings of a reference the module docs state: an exact build version ARN
    /// reaches that build in any status, and a version ARN the latest matching build that
    /// has reached [`super::IMAGE_STATUS_AVAILABLE`]. `None` when `reference` is not an image
    /// ARN of this service, names another Region, or reaches nothing stored.
    #[must_use]
    pub fn resolve_image(
        &self,
        reference: &str,
        request_region: &str,
        at: SystemTime,
    ) -> Option<&ImageBuildVersionRecord> {
        // An image ARN carries the account that owns the image and this store holds the
        // resources of ONE account, so an ARN naming another owner reaches nothing here —
        // Amazon's published catalog included, which is read by its own rule
        // ([`super::managed_image_view`]).
        if ::service_common::arn::parse_of(reference, ARN_SERVICE)
            .is_none_or(|parsed| parsed.account != ::engine::EMULATOR_ACCOUNT_ID)
        {
            return None;
        }
        let reference = super::VersionedReference::parse(reference, IMAGE_ARN_PREFIX)?;
        if reference.region != request_region {
            return None;
        }
        // A regional endpoint serves only its own Region's resources, so a build version of
        // another Region is reached by no reference of this one.
        let named = |record: &&ImageBuildVersionRecord| {
            record.region == request_region
                && reference.name == normalized_name(&record.name)
                && version_admits(&reference.version, &record.version)
        };
        match reference.build_version {
            Some(build_version) => self
                .images
                .values()
                .find(|record| named(record) && record.build_version == build_version),
            None => self
                .images
                .values()
                .filter(|record| {
                    named(record) && record.status_at(at) == IMAGE_STATUS_AVAILABLE
                })
                .max_by_key(|record| {
                    (
                        parse_semantic_version(&record.version).unwrap_or_default(),
                        record.build_version,
                    )
                }),
        }
    }

    /// The build version `record` holds as the modelled `Image` a read reports at the
    /// instant `at`.
    ///
    /// Every member is either what the build recorded or a resource that build referenced,
    /// read back from this store under the Region the build ran in — so what a read reports
    /// about the recipe or the configurations of an image cannot drift from what this service
    /// holds for them, and a reference whose resource has since been deleted reports none.
    #[must_use]
    pub fn image_view(&self, record: &ImageBuildVersionRecord, at: SystemTime) -> Image {
        Image {
            arn: Some(record.arn.clone()),
            r#type: record.image_type.clone(),
            name: Some(record.name.clone()),
            version: Some(record.semantic_version()),
            platform: record.platform.clone(),
            enhanced_image_metadata_enabled: Some(record.enhanced_image_metadata_enabled),
            // "The operating system version for instances that launch from this image":
            // nothing this store holds says which OS version a parent image carries (a
            // recipe's `parentImage` is an AMI id, an ARN or a parameter name), so a build
            // version reports none rather than naming one nothing observed — the same answer
            // a listed summary gives ([`ImageBuildVersionRecord::summary`]).
            os_version: None,
            // "The state of the image", which is read from the clock against the instant the
            // build is due rather than from anything a call had to materialise.
            state: Some(ImageState {
                status: Some(record.status_at(at).to_string()),
                reason: None,
                failure_context: None,
            }),
            // "For images that distribute an AMI, this is the image recipe that Image Builder
            // used to create the image. For container images, this is empty" — and the other
            // way around for `containerRecipe`. Which of the two a build recorded is the kind
            // of recipe it was built from.
            image_recipe: record
                .image_recipe_arn
                .as_deref()
                .and_then(|arn| self.image_recipe(arn, &record.region))
                .map(ImageRecipeRecord::view),
            container_recipe: record
                .container_recipe_arn
                .as_deref()
                .and_then(|arn| self.container_recipe(arn, &record.region))
                .map(ContainerRecipeRecord::view),
            // "The name of the image pipeline that created this image. Image Builder doesn't
            // return this field for new images. Use sourcePipelineArn instead."
            source_pipeline_name: None,
            source_pipeline_arn: record.source_pipeline_arn.clone(),
            infrastructure_configuration: record
                .infrastructure_configuration_arn
                .as_deref()
                .and_then(|arn| self.infrastructure_configuration(arn, &record.region))
                .map(InfrastructureConfigurationRecord::view),
            distribution_configuration: record
                .distribution_configuration_arn
                .as_deref()
                .and_then(|arn| self.distribution_configuration(arn, &record.region))
                .map(DistributionConfigurationRecord::view),
            image_tests_configuration: Some(record.image_tests_configuration.clone()),
            date_created: Some(record.date_created.clone()),
            // "The output resources that Image Builder produces for this image": the AMIs
            // the build registered with Amazon EC2, reported from the instant the build is
            // due on ([`ImageBuildVersionRecord::output_resources`]).
            output_resources: record.output_resources(at),
            tags: Some(reported_tags(record.tags.as_ref())),
            build_type: record.build_type.clone(),
            // "The origin of the base image that Image Builder used to build this image": a
            // recipe's `parentImage` is caller-supplied text that nothing here can place in
            // that enum, so none is reported.
            image_source: None,
            // "Contains information about the current state of scans for this image": no scan
            // of an image runs in this emulator, so there is no scan whose state to report —
            // unlike the scanning SETTINGS below, which are what the build was configured
            // with.
            scan_state: None,
            image_scanning_configuration: record.image_scanning_configuration.clone(),
            // "The time when deprecation occurs for an image resource" and "identifies the
            // last runtime instance of the lifecycle policy to take action on the image":
            // nothing has deprecated this image and no lifecycle execution has acted on it.
            deprecation_time: None,
            lifecycle_execution_id: None,
            execution_role: record.execution_role.clone(),
            workflows: record.workflows.clone(),
            logging_configuration: record.logging_configuration.clone(),
        }
    }

    /// Ask for the build of the build version `arn` to be stopped at the instant `at`, and
    /// answer whether this store holds that build version in `request_region`.
    ///
    /// "Cancellation is asynchronous: the request returns immediately, then Image Builder stops
    /// the running build and moves the image to the CANCELLED state. Output resources that the
    /// build already created, such as AMIs and snapshots, aren't removed"
    /// (`CancelImageCreation`) — so the build is the running one until its cancellation is
    /// due ([`super::IMAGE_CANCELLATION_SPAN`]) and reports
    /// [`super::IMAGE_STATUS_CANCELLED`] from that instant on, which the clock decides
    /// ([`ImageBuildVersionRecord::status_at`]); the build it stopped never reaches its
    /// available instant, and what that build already registered stays on the record for a
    /// re-run ([`Self::rerun_image_build`]) to replace.
    ///
    /// The PHASE the build was running is recorded on it as well
    /// ([`ImageBuildVersionRecord::stopped_in_phase`]), since that is the stage of the image
    /// creation process the build stopped at, and what decides whether a retry may resume it
    /// ([`ImageBuildVersionRecord::stopped_in_phase_at`]). A build this emulator runs is in its
    /// build phase from the instant it was queued until it is due
    /// ([`super::IMAGE_BUILD_SPAN`]), and only a build that has not ended is stopped here, so
    /// the phase a cancellation lands in is that one. The phase the cancellation asked for
    /// FIRST is the one that stands, since that is the cancellation that is due.
    pub fn cancel_image_build(&mut self, arn: &str, request_region: &str, at: SystemTime) -> bool {
        let Some(record) = self
            .images
            .get_mut(arn)
            .filter(|record| record.region == request_region)
        else {
            return false;
        };
        record
            .stopped_in_phase
            .get_or_insert_with(|| super::IMAGE_PHASE_BUILDING.to_string());
        let due = at.checked_add(super::IMAGE_CANCELLATION_SPAN);
        record.cancelled_at = match (record.cancelled_at, due) {
            // A build already being stopped is not made to run longer by a second request to
            // stop it: the cancellation asked for first is the one that is due.
            (Some(asked), Some(due)) => Some(asked.min(due)),
            (asked, due) => asked.or(due),
        };
        true
    }

    /// Re-run the build of the build version `arn` in place at the instant `at`, and answer
    /// whether this store holds that build version in `request_region`.
    ///
    /// "The image re-runs asynchronously in place: the same build version returns to the test
    /// or distribution phase where it failed and continues from there. No new image build
    /// version is created" (`RetryImage`) — so the SAME record is the one that runs: the
    /// status it ended in, the phase its stopped run ended in
    /// ([`ImageBuildVersionRecord::stopped_in_phase`]) and any cancellation that stopped it are
    /// no longer what it reports, its build is outstanding again until it is due
    /// ([`super::IMAGE_BUILD_SPAN`]), and what the ended run produced is no longer what this
    /// build version produced.
    pub fn rerun_image_build(
        &mut self,
        arn: &str,
        request_region: &str,
        at: SystemTime,
    ) -> bool {
        let Some(record) = self
            .images
            .get_mut(arn)
            .filter(|record| record.region == request_region)
        else {
            return false;
        };
        record.terminal_status = None;
        record.cancelled_at = None;
        record.stopped_in_phase = None;
        record.available_at = at.checked_add(super::IMAGE_BUILD_SPAN);
        record.output_amis.clear();
        true
    }
}
