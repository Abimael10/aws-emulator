// SPDX-License-Identifier: CC0-1.0

//! The `InputSource` side of this service's store.
//!
//! An input source is created "for a service" (`CreateInputSource`'s own documentation)
//! and listed per `serviceArn`, so one [`InputSourceRecord`] per created source lives
//! INSIDE the service it was created for ([`super::ServiceRecord::input_sources`]), the
//! same shape the `ServiceFunction` side of this store uses: deleting the service removes
//! its input sources with it, and no lookup can reach a source whose service is gone.
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

use std::time::SystemTime;

use crate::ops::types::{InputSourceSummary, ResourceConfiguration};

use super::{to_date_time, ServiceRecord};

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

/// One created input source, and the sole source of every `InputSourceSummary` body this
/// crate returns.
#[derive(Debug, Clone)]
pub struct InputSourceRecord {
    /// The minted `inputSourceId`; also this record's key in
    /// [`super::ServiceRecord::input_sources`].
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

impl ServiceRecord {
    /// This service's input sources in creation order — what `ListInputSources`
    /// enumerates before it filters and projects them.
    pub fn input_sources_in_creation_order(&self) -> Vec<&InputSourceRecord> {
        let mut sources: Vec<&InputSourceRecord> = self.input_sources.values().collect();
        sources.sort_by_key(|source| source.sequence);
        sources
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
