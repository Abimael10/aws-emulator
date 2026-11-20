// SPDX-License-Identifier: CC0-1.0

//! The segment definitions a domain holds: the criteria `CreateSegmentDefinition`
//! persisted, the ARN it minted, and the sentences every operation addressing one words
//! its refusals with.
//!
//! A segment definition is a resource OF its domain, identified by the
//! (`DomainName`, `SegmentDefinitionName`) pair — the name is the `@httpLabel` of every
//! segment-definition operation — so the records live on [`super::Domain`] and are dropped
//! with it.
//!
//! `SegmentType` is NOT stored: `GetSegmentDefinitionResponse$SegmentType` documents the
//! two kinds a definition can be ("Classic : Segments created using traditional
//! SegmentGroup structure", "Enhanced : Segments created using SQL queries"), so the kind
//! follows from WHICH criteria the create carried and is resolved on every read by
//! [`StoredSegmentDefinition::segment_type`].

use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::{SegmentGroup, SegmentSort};

/// Emulator resource guard, NOT an AWS quota: the most segment definitions this emulator
/// stores for one domain. Set well above any realistic emulator workload, and checked
/// before the insert so repeated `CreateSegmentDefinition` calls cannot grow the store
/// without bound.
pub const MAX_SEGMENT_DEFINITIONS_PER_DOMAIN: usize = 1_000;

/// The `SegmentType` of a definition whose criteria are the segment-group structure —
/// "Classic : Segments created using traditional SegmentGroup structure".
pub const SEGMENT_TYPE_CLASSIC: &str = "CLASSIC";

/// The `SegmentType` of a definition whose criteria are a SQL query —
/// "Enhanced : Segments created using SQL queries".
pub const SEGMENT_TYPE_ENHANCED: &str = "ENHANCED";

/// One stored segment definition: the criteria and labels `CreateSegmentDefinition`
/// persisted, the ARN minted for it, and the instant it was created.
#[derive(Debug, Clone)]
pub struct StoredSegmentDefinition {
    /// `SegmentDefinitionName`, as stored (also this record's key in
    /// [`super::Domain::segment_definitions`]).
    pub name: String,
    /// `SegmentDefinitionArn`: minted once, when the definition was created, so a later
    /// read reports the ARN the definition was created with.
    pub arn: String,
    /// `DisplayName`, as stored. The member is required of a create, so every stored
    /// definition carries one.
    pub display_name: String,
    /// `Description`, as stored.
    pub description: Option<String>,
    /// `SegmentGroups`, as stored: the criteria of a classic definition.
    pub segment_groups: Option<SegmentGroup>,
    /// `SegmentSqlQuery`, as stored: the criteria of an enhanced definition.
    pub segment_sql_query: Option<String>,
    /// `SegmentSort`, as stored.
    pub segment_sort: Option<SegmentSort>,
    /// `Tags`, as stored.
    pub tags: Option<HashMap<String, String>>,
    /// `CreatedAt`: generated once, when the definition was created, and never
    /// recomputed.
    pub created_at: SystemTime,
}

impl StoredSegmentDefinition {
    /// `SegmentType` as the wire reports it: the kind of criteria this definition was
    /// created with — a SQL query makes it enhanced, the segment-group structure makes it
    /// classic.
    pub fn segment_type(&self) -> String {
        if self.segment_sql_query.is_some() {
            SEGMENT_TYPE_ENHANCED.to_string()
        } else {
            SEGMENT_TYPE_CLASSIC.to_string()
        }
    }

    /// `Tags` as a segment-definition projection reports it: the tag map this definition
    /// carries, PRESENT and empty for a definition that carries none, exactly as
    /// [`super::Domain::reported_tags`] reports a domain's.
    ///
    /// Unlike the definition's other optional members — `Description`, `SegmentGroups`,
    /// `SegmentSqlQuery`, `SegmentSort`, each absent from the response when the create did
    /// not carry it — the tag map is always reported, so a caller that indexes `Tags` can
    /// tell "this definition carries no tags" from "tags were not reported".
    pub fn reported_tags(&self) -> Option<HashMap<String, String>> {
        Some(self.tags.clone().unwrap_or_default())
    }
}

/// One segment definition's `SegmentDefinitionArn`: this service's ARN namespace, the
/// region the definition was created in, and the domain-scoped resource path the
/// definition is addressed by — the same path its operations are bound to.
pub fn segment_definition_arn(
    region: &str,
    domain_name: &str,
    segment_definition_name: &str,
) -> String {
    ::engine::emulator_arn(
        super::ARN_SERVICE,
        region,
        &format!("domains/{domain_name}/segment-definitions/{segment_definition_name}"),
    )
}

/// The `ResourceNotFoundException` message for a segment definition a domain does not
/// hold, as the service words that refusal: the resource kind as the API spells it and
/// "is not found", whose only variable part is the name the request's
/// `{SegmentDefinitionName}` label carried. The domain is the one the request addressed,
/// so it is not restated. Shared by every operation addressing one definition, so the
/// same absent name reads the same whichever of them was called.
pub fn segment_definition_not_found(segment_definition_name: &str) -> String {
    format!("SegmentDefinition {segment_definition_name} is not found")
}

/// The `BadRequestException` message for a create that carries NEITHER kind of criteria:
/// the service's fixed sentence for a request that names no segment source, qualified by
/// the two identifiers the operation is addressed by — the `{DomainName}` and
/// `{SegmentDefinitionName}` labels of the request — so a caller issuing many creates can
/// tell WHICH of them was refused.
pub fn segment_criteria_required(domain_name: &str, segment_definition_name: &str) -> String {
    format!(
        "Must specify either SegmentSqlQuery or SegmentGroups. Domain: {domain_name}, \
         SegmentDefinitionName: {segment_definition_name}"
    )
}

/// The `BadRequestException` message for a segment definition name the domain already
/// holds: the service's fixed sentence for an already-existing definition, with the name
/// the request's `{SegmentDefinitionName}` label carried as its only variable part.
pub fn segment_definition_exists(segment_definition_name: &str) -> String {
    format!("Segment definition with name {segment_definition_name} already exists")
}
