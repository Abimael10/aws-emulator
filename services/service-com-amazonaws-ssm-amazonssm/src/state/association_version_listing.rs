// SPDX-License-Identifier: CC0-1.0

//! Listing the versions of ONE State Manager association: the modelled identity each
//! version is reported by, and the page a `ListAssociationVersions` read is served.
//!
//! The sibling listing in [`super::associations`] walks the associations of a Region; this
//! one walks the versions of one association, the way [`super::document_version_listing`]
//! walks the versions of one document. It walks them NEWEST VERSION FIRST — the version in
//! force is what a client reading a history asks about first, which is the order Systems
//! Manager serves one in — so a page's resume cursor is still total: the cursor is the
//! VERSION NUMBER the next page starts at, not an offset into a history an update may have
//! grown between two pages.
//!
//! Each version is reported by [`association_version_info`], read from the same record
//! [`super::associations::AssociationRecord::to_association_description`] describes a
//! version from, so a version's identity in a listing cannot disagree with its description.

use crate::ops::types::AssociationVersionInfo;

use super::associations::{AssociationRecord, AssociationVersionRecord};

/// The modelled `AssociationVersionInfo` of `version` of `record`: the version's own
/// members beside the id and the document name of the association that holds it.
///
/// Every member is read from the configuration `version` holds — the association's whole
/// configuration as of that version ([`AssociationVersionRecord::carried_forward`]) — so a
/// member nothing ever set is left unset rather than invented.
/// `AssociationVersionInfo` carries no per-node status and no overview, so nothing here
/// depends on the instant it is read at.
pub fn association_version_info(
    record: &AssociationRecord,
    version: &AssociationVersionRecord,
) -> AssociationVersionInfo {
    AssociationVersionInfo {
        // "The ID created by the system when the association was created": the id belongs to
        // the association rather than to one of its versions. The document, though, is the
        // one THIS version applies, since an update can move the association onto another
        // ([`AssociationVersionRecord::document_name`]).
        association_id: Some(record.association_id.clone()),
        association_version: Some(version.version.to_string()),
        created_date: Some(crate::state::wire_instant(version.created_at)),
        name: Some(version.document_name.clone()),
        document_version: version.document_version.clone(),
        parameters: (!version.parameters.is_empty()).then(|| {
            version
                .parameters
                .iter()
                .map(|(name, values)| (name.clone(), values.clone()))
                .collect()
        }),
        targets: (!version.targets.is_empty()).then(|| version.targets.clone()),
        schedule_expression: version.schedule_expression.clone(),
        output_location: version.output_location.clone(),
        association_name: version.association_name.clone(),
        max_errors: version.max_errors.clone(),
        max_concurrency: version.max_concurrency.clone(),
        compliance_severity: version.compliance_severity.clone(),
        sync_compliance: version.sync_compliance.clone(),
        apply_only_at_cron_interval: version.apply_only_at_cron_interval,
        // "The names or Amazon Resource Names (ARNs) of the Change Calendar type documents
        // your associations are gated under" (`AssociationVersionInfo$CalendarNames`, a
        // `CalendarNameOrARNList` with no length bound): an association that no change
        // calendar gates is gated under NONE of them, which is an empty list rather than no
        // list at all — so a client iterating the member never has to tell a missing one
        // from an empty one.
        calendar_names: Some(version.calendar_names.clone()),
        target_locations: (!version.target_locations.is_empty())
            .then(|| version.target_locations.clone()),
        schedule_offset: version.schedule_offset,
        duration: version.duration,
        target_maps: (!version.target_maps.is_empty()).then(|| version.target_maps.clone()),
        association_dispatch_assume_role: version.association_dispatch_assume_role.clone(),
    }
}

/// One page of an association's version listing: the versions it reports, and the version
/// the next page resumes at when one was left over.
#[derive(Debug, Clone, Default)]
pub struct AssociationVersionPage {
    pub versions: Vec<AssociationVersionInfo>,
    /// "The token for the next set of items to return" — `None` when this page is the last,
    /// so a caller that reads the whole history stops rather than asking again.
    pub next_token: Option<String>,
}

impl AssociationRecord {
    /// The page of this association's versions starting at the version `resume_from` numbers
    /// and holding at most `page_size` of them, NEWEST FIRST.
    ///
    /// The walk resumes at the newest version still held at or before `resume_from`, so a
    /// page keeps making forward progress whatever the history holds.
    pub fn list_versions(
        &self,
        resume_from: Option<u64>,
        page_size: usize,
    ) -> AssociationVersionPage {
        let mut versions = Vec::new();
        let mut next_token = None;
        // The store holds the versions oldest first, in the order a create and its updates
        // minted them; a listing serves them the other way round.
        for version in self.versions.iter().rev() {
            if resume_from.is_some_and(|from| version.version > from) {
                continue;
            }
            if versions.len() == page_size {
                // The page is full and a version is still waiting: hand the caller the
                // version it resumes at, so the next request continues where this stopped.
                next_token = Some(version.version.to_string());
                break;
            }
            versions.push(association_version_info(self, version));
        }
        AssociationVersionPage {
            versions,
            next_token,
        }
    }
}
