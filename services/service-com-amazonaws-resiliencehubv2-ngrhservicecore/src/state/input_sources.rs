// SPDX-License-Identifier: CC0-1.0

//! The `InputSource` side of this service's store.
//!
//! An input source is created "for a service" (`CreateInputSource`'s own documentation)
//! and listed per `serviceArn`, but it OUTLIVES that service: one
//! [`InputSourceCollection`] per `serviceArn` lives in the store beside the service
//! records ([`super::ComAmazonawsResiliencehubv2NgrhservicecoreState::input_sources`])
//! rather than inside one, so deleting the service leaves its input sources addressable —
//! and deletable — through the very same ARN. That is what separates them from the
//! `ServiceFunction` and `Assertion` sides of this store, which are owned by the service
//! record and go with it.
//!
//! A collection whose service has been deleted is ORPHANED
//! ([`InputSourceCollection::orphaned_at`]): it still answers reads and deletes, but no
//! create can add to it, because a create resolves a live service first. The store keeps
//! at most [`MAX_ORPHANED_INPUT_SOURCE_SERVICES`] of them, dropping the longest-orphaned
//! first, so the collections cannot grow without limit as services come and go.
//!
//! What a record holds is the `ResourceConfiguration` the create supplied, unchanged. That
//! union carries EXACTLY ONE member ("Resource configuration for an input source. Provide
//! exactly one field."), and that one member decides both which configuration member a
//! summary reports back and which `InputSourceType` it is — see
//! [`InputSourceRecord::source_type`] — so the type and the configuration can never
//! disagree. Nothing else identifies a source: two creates carrying identical
//! configuration are two distinct input sources, each with its own minted
//! `inputSourceId`, because the model publishes no uniqueness over the configuration.
//!
//! Every `InputSourceSummary` this crate returns is projected from a record here through
//! [`InputSourceRecord::to_summary`], so a listing cannot drift from what a create stored.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::{InputSourceSummary, ResourceConfiguration};

use super::{to_date_time, ComAmazonawsResiliencehubv2NgrhservicecoreState};

/// `InputSourceType` for a source discovered from `resourceTags` ("The resource tags for
/// tag-based resource discovery"). One of `INPUT_SOURCE_TYPE_VALUES`.
pub const INPUT_SOURCE_TYPE_TAGS: &str = "TAGS";

/// `InputSourceType` for a source discovered from a CloudFormation stack (`cfnStackArn`).
pub const INPUT_SOURCE_TYPE_CFN_STACK: &str = "CFN_STACK";

/// `InputSourceType` for a source discovered from a Terraform state file
/// (`tfStateFileUrl`).
pub const INPUT_SOURCE_TYPE_TERRAFORM: &str = "TERRAFORM";

/// `InputSourceType` for a source discovered from an Amazon EKS cluster (`eks`).
pub const INPUT_SOURCE_TYPE_EKS: &str = "EKS";

/// `InputSourceType` for a source discovered from a design file (`designFileS3Url`).
pub const INPUT_SOURCE_TYPE_DESIGN_FILE: &str = "DESIGN_FILE";

/// The ceiling on how many input sources one service will hold, so an unauthenticated
/// caller cannot grow the process's memory without limit by repeating
/// `CreateInputSource`.
///
/// CHOSEN, not documented: the model publishes no per-service input-source quota, so this
/// mirrors the ceiling the `ServiceFunction` side of this store uses
/// ([`super::MAX_SERVICE_FUNCTIONS_PER_SERVICE`]). `CreateInputSource` refuses a create
/// that would pass it with the `ServiceQuotaExceededException` (HTTP 402) its model
/// declares. The per-service idempotency index is bounded by the same ceiling: a replayed
/// token returns the stored source instead of inserting a second one.
pub const MAX_INPUT_SOURCES_PER_SERVICE: usize = super::MAX_SERVICE_FUNCTIONS_PER_SERVICE;

/// The ceiling on how many input-source collections whose service has been deleted the
/// store retains, so a caller repeating create-service / create-input-source /
/// delete-service cannot grow the process's memory without limit through the sources that
/// survive each delete.
///
/// CHOSEN, not documented: the model publishes no quota over input sources that outlive
/// their service, so this mirrors the ceiling the `Service` side of this store uses
/// ([`super::MAX_SERVICES`]) — as many orphaned collections as there are services the
/// store will hold at once. Past it the LONGEST-orphaned collection is dropped, never one
/// whose service still exists, so the ARNs a caller is working with keep their sources.
pub const MAX_ORPHANED_INPUT_SOURCE_SERVICES: usize = super::MAX_SERVICES;

/// One created input source, and the sole source of every `InputSourceSummary` body this
/// crate returns.
#[derive(Debug, Clone)]
pub struct InputSourceRecord {
    /// The minted `inputSourceId`; also this record's key in
    /// [`InputSourceCollection::sources`].
    pub input_source_id: String,
    /// The resource configuration this source discovers from, exactly as the create
    /// supplied it.
    pub configuration: ResourceConfiguration,
    pub created_at: SystemTime,
    /// Creation order, from the store's monotonic counter. `ListInputSources` scans by
    /// this and its `nextToken` names a position in it, so a page resumes the same scan
    /// even after a source is deleted.
    pub sequence: u64,
}

impl InputSourceRecord {
    /// The `InputSourceType` this record's configuration names.
    ///
    /// The type is DERIVED from the stored configuration rather than held beside it: the
    /// union's one member is what a source discovers from, so the reported type is that
    /// member's own type and the two cannot disagree. `MONITORING` is not among them —
    /// `ResourceConfiguration` publishes no member for it, so no create can produce one.
    pub fn source_type(&self) -> &'static str {
        match self.configuration {
            ResourceConfiguration::resourceTags(_) => INPUT_SOURCE_TYPE_TAGS,
            ResourceConfiguration::cfnStackArn(_) => INPUT_SOURCE_TYPE_CFN_STACK,
            ResourceConfiguration::tfStateFileUrl(_) => INPUT_SOURCE_TYPE_TERRAFORM,
            ResourceConfiguration::Eks(_) => INPUT_SOURCE_TYPE_EKS,
            ResourceConfiguration::designFileS3Url(_) => INPUT_SOURCE_TYPE_DESIGN_FILE,
        }
    }

    /// This record as the `InputSourceSummary` shape `ListInputSources` enumerates.
    ///
    /// Exactly the one configuration member the create supplied is reported; the other
    /// four stay absent rather than carrying a value nothing stored, which is what makes
    /// `type` and the configuration two readings of the same stored member.
    pub fn to_summary(&self) -> InputSourceSummary {
        let mut summary = InputSourceSummary {
            input_source_id: self.input_source_id.clone(),
            r#type: Some(self.source_type().to_string()),
            resource_tags: None,
            cfn_stack_arn: None,
            tf_state_file_url: None,
            eks: None,
            design_file_s3_url: None,
            created_at: Some(to_date_time(self.created_at)),
        };
        match &self.configuration {
            ResourceConfiguration::resourceTags(tags) => {
                summary.resource_tags = Some(tags.clone());
            }
            ResourceConfiguration::cfnStackArn(arn) => {
                summary.cfn_stack_arn = Some(arn.clone());
            }
            ResourceConfiguration::tfStateFileUrl(url) => {
                summary.tf_state_file_url = Some(url.clone());
            }
            ResourceConfiguration::Eks(source) => {
                summary.eks = Some(source.clone());
            }
            ResourceConfiguration::designFileS3Url(url) => {
                summary.design_file_s3_url = Some(url.clone());
            }
        }
        summary
    }
}

/// The input sources created for ONE `serviceArn`, and the `clientToken` index over them.
///
/// Keyed in the store by that `serviceArn` rather than held inside the service record, so
/// the collection survives `DeleteService` and the reads and deletes addressed to the ARN
/// keep resolving.
#[derive(Debug, Clone, Default)]
pub struct InputSourceCollection {
    /// The input sources created for this `serviceArn`, keyed by `inputSourceId`.
    pub sources: BTreeMap<String, InputSourceRecord>,
    /// `clientToken` -> `inputSourceId`, so a replayed `CreateInputSource`
    /// `@idempotencyToken` returns the input source the first call created instead of
    /// creating a second one. Scoped to the `serviceArn` the create named.
    pub idempotency: BTreeMap<String, String>,
    /// The store position at which this collection's service was deleted, and `None` while
    /// that service still exists. It orders the orphans for
    /// [`MAX_ORPHANED_INPUT_SOURCE_SERVICES`], so the longest-orphaned collection is the
    /// one dropped at the ceiling.
    pub orphaned_at: Option<u64>,
}

impl InputSourceCollection {
    /// These input sources in creation order — what `ListInputSources` enumerates before
    /// it filters and projects them.
    pub fn in_creation_order(&self) -> Vec<&InputSourceRecord> {
        let mut sources: Vec<&InputSourceRecord> = self.sources.values().collect();
        sources.sort_by_key(|source| source.sequence);
        sources
    }
}

impl ComAmazonawsResiliencehubv2NgrhservicecoreState {
    /// Whether `service_arn` is an ARN this store can answer an input-source read or delete
    /// for: either the service is still stored, or input sources created for it survive it.
    /// An ARN that has neither is what a `ResourceNotFoundException` names.
    pub fn input_sources_addressable(&self, service_arn: &str) -> bool {
        self.services.contains_key(service_arn) || self.input_sources.contains_key(service_arn)
    }

    /// The input sources held for `service_arn`, whether or not its service still exists.
    pub fn input_sources_for(&self, service_arn: &str) -> Option<&InputSourceCollection> {
        self.input_sources.get(service_arn)
    }

    /// The input sources held for `service_arn`, mutably.
    pub fn input_sources_for_mut(
        &mut self,
        service_arn: &str,
    ) -> Option<&mut InputSourceCollection> {
        self.input_sources.get_mut(service_arn)
    }

    /// The collection a create stores into, opened empty the first time `service_arn` gets
    /// an input source. Only a create reaches this, and a create resolves a live service
    /// first, so no collection is ever opened for an ARN that names no service.
    pub fn input_sources_entry(&mut self, service_arn: &str) -> &mut InputSourceCollection {
        self.input_sources.entry(service_arn.to_string()).or_default()
    }

    /// Record that `service_arn`'s service has been deleted while its input sources remain.
    ///
    /// The sources are NOT touched: they outlive the service, so the collection only takes
    /// its orphan position, and the store then drops the longest-orphaned collections past
    /// [`MAX_ORPHANED_INPUT_SOURCE_SERVICES`]. An ARN that never had an input source keeps
    /// no collection at all, so a later read of it is still refused.
    pub fn orphan_input_sources(&mut self, service_arn: &str) {
        if !self.input_sources.contains_key(service_arn) {
            return;
        }
        let position = self.next_sequence();
        if let Some(collection) = self.input_sources.get_mut(service_arn) {
            collection.orphaned_at = Some(position);
        }
        self.prune_orphaned_input_sources();
    }

    /// Drop the longest-orphaned input-source collections while more of them are retained
    /// than [`MAX_ORPHANED_INPUT_SOURCE_SERVICES`] allows.
    fn prune_orphaned_input_sources(&mut self) {
        let mut orphans: Vec<(u64, String)> = self
            .input_sources
            .iter()
            .filter_map(|(arn, collection)| {
                collection.orphaned_at.map(|position| (position, arn.clone()))
            })
            .collect();
        if orphans.len() <= MAX_ORPHANED_INPUT_SOURCE_SERVICES {
            return;
        }
        orphans.sort_by_key(|(position, _)| *position);
        let excess = orphans.len() - MAX_ORPHANED_INPUT_SOURCE_SERVICES;
        for (_, arn) in orphans.into_iter().take(excess) {
            self.input_sources.remove(&arn);
        }
    }
}

/// Mint a fresh `inputSourceId`.
///
/// `InputSourceId` is constrained `len 1..255` / `^\S{1,255}$`, which a UUID from
/// `mint_uuid` satisfies; none of its bits comes from the request, so two sources created
/// from identical configuration are still distinct resources.
pub fn mint_input_source_id(region: &str) -> String {
    super::mint_uuid(region)
}
